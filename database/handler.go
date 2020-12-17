package database

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"io/ioutil"
	"log"
	"os"
	"path"
	"time"

	"github.com/ag-computational-bio/bakta-web-api/go/api"

	"github.com/google/uuid"
	"github.com/spf13/viper"

	"gorm.io/driver/postgres"
	"gorm.io/driver/sqlite"
	"gorm.io/gorm"
)

// BackendType The type of the database backend to use
type BackendType string

const (
	//SQLite Use an sqlite database in the backend
	SQLite BackendType = "SQLite"
	//Postgres User a postgres database in the backend
	Postgres BackendType = "Postgres"
)

//UploadFileType type of file to upload
type UploadFileType string

const (
	//Fasta fasta file
	Fasta UploadFileType = "fasta"
	//Replicon replicon file as tsv, see bakta documentation for further details
	Replicon UploadFileType = "replicons"
	//Prodigal Prodigal training file, see bakta documentation for further details
	Prodigal UploadFileType = "prodigal"
)

const resultFileName = "results.tar.gz"

//Handler Wraps the database with convinence methods
type Handler struct {
	DB         *gorm.DB
	BaseKey    string
	DataBucket string
}

// InitDatabaseHandler Initializes the database to store the Job
func InitDatabaseHandler() (*Handler, error) {

	var db *gorm.DB
	var err error

	databaseType := viper.GetString("Database.Backend")

	switch databaseType {
	case string(SQLite):
		db, err = createSQLiteDatabase()
	case string(Postgres):
		db, err = createPostgresSQL()
	}

	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = db.AutoMigrate(&Job{})
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	bucket := viper.GetString("Objectstorage.S3.Bucket")
	baseKey := viper.GetString("Objectstorage.S3.BaseKey")

	dbHandler := Handler{
		DB:         db,
		DataBucket: bucket,
		BaseKey:    baseKey,
	}

	return &dbHandler, nil
}

func createSQLiteDatabase() (*gorm.DB, error) {
	tmpDir, err := ioutil.TempDir("", "baktadb-*")
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}
	db, err := gorm.Open(sqlite.Open(path.Join(tmpDir, "baktadb.db")), &gorm.Config{})
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return db, nil
}

func createPostgresSQL() (*gorm.DB, error) {
	host := os.Getenv("DatabaseHost")
	dbName := os.Getenv("DBName")
	dbUser := os.Getenv("DBUser")
	dbPassword := os.Getenv("DBPassword")
	dbPort := os.Getenv("DBPort")

	dsn := fmt.Sprintf("host=%v user=%v password=%v dbname=%v port=%v sslmode=disable TimeZone=Europe/Berlin", host, dbUser, dbPassword, dbName, dbPort)
	db, err := gorm.Open(postgres.Open(dsn), &gorm.Config{})
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return db, nil
}

//CreateJob Creates a new bakta job in init mode
func (handler *Handler) CreateJob() (*Job, string, error) {
	jobID := uuid.New()
	secretID, err := randStringBytes(50)
	if err != nil {
		log.Println(err.Error())
		return nil, "", err
	}

	secretSHA := sha256.Sum256([]byte(secretID))
	secretSHABase64 := base64.StdEncoding.EncodeToString(secretSHA[:])

	job := Job{
		JobID:       jobID.String(),
		Secret:      secretSHABase64,
		DataBucket:  handler.DataBucket,
		FastaKey:    handler.createUploadStoreKey(jobID.String(), Fasta),
		ProdigalKey: handler.createUploadStoreKey(jobID.String(), Prodigal),
		RepliconKey: handler.createUploadStoreKey(jobID.String(), Replicon),
		ResultKey:   handler.createResultStoreKey(jobID.String()),
		Status:      api.JobStatusEnum_INIT.String(),
		ExpiryDate:  time.Now().AddDate(0, 0, 10),
	}

	result := handler.DB.Create(&job)
	if result.Error != nil {
		log.Println(result.Error)
		return nil, "", err
	}

	getDataResult := handler.DB.First(&job)
	if getDataResult.Error != nil {
		log.Println(getDataResult.Error)
		return nil, "", err
	}

	return &job, secretID, nil
}

//UpdateK8s Updates a job with its k8s id
func (handler *Handler) UpdateK8s(id string, k8s string) error {
	job := Job{
		JobID: id,
	}

	getDataResult := handler.DB.First(&job)
	if getDataResult.Error != nil {
		log.Println(getDataResult.Error)
		return getDataResult.Error
	}

	job.K8sID = k8s
	job.Status = api.JobStatusEnum_RUNNING.String()

	handler.DB.Save(&job)

	return nil
}

//UpdateStatus Updates the status of a job
func (handler *Handler) UpdateStatus(id string, status api.JobStatusEnum, errorMsg string) error {
	job := Job{
		JobID: id,
	}

	updateResult := handler.DB.Model(&job).Updates(Job{Status: status.String(), Error: errorMsg})
	if updateResult.Error != nil {
		log.Println(updateResult.Error)
		return updateResult.Error
	}

	return nil
}

//GetJob Returns the stored config of a job
func (handler *Handler) GetJob(id string) (*Job, error) {
	job := Job{}
	job.JobID = id

	result := handler.DB.First(&job)
	if result.Error != nil {
		log.Println(result.Error)
		return nil, result.Error
	}

	return &job, nil
}

//CheckSecret Compares the provided secret/JobID with a job in the database
func (handler *Handler) CheckSecret(id string, secretKey string) error {
	job := Job{
		JobID: id,
	}

	getDataResult := handler.DB.First(&job)
	if getDataResult.Error != nil {
		log.Println(getDataResult.Error)
		return getDataResult.Error
	}

	secretSHA := sha256.Sum256([]byte(secretKey))
	secretSHABase64 := base64.StdEncoding.EncodeToString(secretSHA[:])

	if secretSHABase64 != job.Secret {
		return errors.New("Wrong secret provided")
	}

	return nil
}

func (handler *Handler) GetJobsStatus(jobIDs []string) ([]Job, error) {
	var jobs []Job

	connection := handler.DB.Where("job_id IN ?", jobIDs).Find(&jobs)
	if connection.Error != nil {
		log.Println(connection.Error)
		return nil, connection.Error
	}

	return jobs, nil
}

func (handler *Handler) GetJobStatus(jobID string) (*Job, error) {
	job := Job{
		JobID: jobID,
	}

	connection := handler.DB.First(&job)
	if connection.Error != nil {
		log.Println(connection.Error)
		return nil, connection.Error
	}

	return &job, nil
}

func (handler *Handler) createUploadStoreKey(id string, uploadFileType UploadFileType) string {
	var filename string
	switch uploadFileType {
	case Fasta:
		filename = "fastadata.fasta"
	case Replicon:
		filename = "replicons.tsv"
	case Prodigal:
		filename = "prodigaltraining.protf"
	}

	resultKey := path.Join(handler.BaseKey, "uploaddata", id, filename)
	return resultKey
}

func (handler *Handler) createResultStoreKey(id string) string {
	resultKey := path.Join(handler.BaseKey, "results", id, resultFileName)
	return resultKey
}

func randStringBytes(n int) (string, error) {
	b := make([]byte, n)
	_, err := rand.Read(b)
	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	data := base64.StdEncoding.EncodeToString(b)

	return data, nil
}
