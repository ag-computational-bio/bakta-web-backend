package database

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"log"
	"os"
	"path"
	"time"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/google/uuid"
	"github.com/spf13/viper"
	"go.mongodb.org/mongo-driver/bson"
	"go.mongodb.org/mongo-driver/bson/primitive"
	"go.mongodb.org/mongo-driver/mongo"
	"go.mongodb.org/mongo-driver/mongo/options"
	"go.mongodb.org/mongo-driver/mongo/readpref"
)

// BackendType The type of the database backend to use
type BackendType string

//UploadFileType type of file to upload
type UploadFileType string

const (
	//Fasta fasta file
	Fasta UploadFileType = "fasta"
	//Replicon replicon file as tsv, see bakta documentation for further details
	RepliconCSV UploadFileType = "repliconcsv"
	RepliconTSV UploadFileType = "replicontsv"
	//Prodigal Prodigal training file, see bakta documentation for further details
	Prodigal UploadFileType = "prodigal"
)

const RESULTFILENAME = "results.tar.gz"
const COLLECTIONNAME = "jobs"

//Handler Wraps the database with convinence methods
type Handler struct {
	DB             *mongo.Client
	Collection     *mongo.Collection
	BaseKey        string
	UserDataBucket string
	DBBucket       string
	ExpiryTime     int64
}

// InitDatabaseHandler Initializes the database to store the Job
func InitDatabaseHandler() (*Handler, error) {
	host := viper.GetString("Database.MongoHost")
	dbName := viper.GetString("Database.MongoDBName")
	dbUser := viper.GetString("Database.MongoUser")
	dbAuthSource := viper.GetString("Database.MongoAuthSource")
	dbPassword := os.Getenv("MongoPassword")
	dbPort := viper.GetString("Database.MongoPort")

	if dbPassword == "" {
		return nil, fmt.Errorf("password for mongodb required, can be set with env var MongoPassword")
	}

	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)
	client, err := mongo.Connect(ctx, options.Client().ApplyURI(fmt.Sprintf("mongodb://%v:%v", host, dbPort)).SetAuth(
		options.Credential{
			AuthSource: dbAuthSource,
			Username:   dbUser,
			Password:   dbPassword,
		},
	))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = client.Ping(ctx, readpref.Primary())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	collection := client.Database(dbName).Collection(COLLECTIONNAME)

	userBucket := viper.GetString("Objectstorage.S3.UserBucket")
	baseKey := viper.GetString("Objectstorage.S3.BaseKey")
	expiryTime := viper.GetInt64("ExpiryTime")

	dbHandler := Handler{
		DB:             client,
		Collection:     collection,
		UserDataBucket: userBucket,
		BaseKey:        baseKey,
		ExpiryTime:     expiryTime,
	}

	return &dbHandler, nil
}

//CreateJob Creates a new bakta job in init mode
func (handler *Handler) CreateJob(repliconTypeAPI api.RepliconTableType, jobname string) (*Job, string, error) {
	jobID := uuid.New()
	secretID, err := randStringBytes(50)
	if err != nil {
		log.Println(err.Error())
		return nil, "", err
	}

	secretSHA := sha256.Sum256([]byte(secretID))
	secretSHABase64 := base64.StdEncoding.EncodeToString(secretSHA[:])

	repliconType := RepliconTSV

	switch repliconTypeAPI {
	case api.RepliconTableType_CSV:
		repliconType = RepliconCSV
	case api.RepliconTableType_TSV:
		repliconType = RepliconTSV
	}

	job := Job{
		JobID:       jobID.String(),
		Secret:      secretSHABase64,
		DataBucket:  handler.UserDataBucket,
		FastaKey:    handler.createUploadStoreKey(jobID.String(), Fasta),
		ProdigalKey: handler.createUploadStoreKey(jobID.String(), Prodigal),
		RepliconKey: handler.createUploadStoreKey(jobID.String(), repliconType),
		ResultKey:   handler.createResultStoreKey(jobID.String()),
		Status:      api.JobStatusEnum_INIT.String(),
		ExpiryDate:  primitive.Timestamp{T: uint32(time.Now().AddDate(0, 0, 10).Unix())},
		Created:     primitive.Timestamp{T: uint32(time.Now().Unix())},
		Updated:     primitive.Timestamp{T: uint32(time.Now().Unix())},
		Jobname:     jobname,
	}

	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)
	inserted, err := handler.Collection.InsertOne(ctx, job)
	if err != nil {
		log.Println(err.Error())
		return nil, "", err
	}

	inserted_query := bson.M{
		"_id": inserted.InsertedID,
	}

	result := handler.Collection.FindOne(ctx, inserted_query)
	if result.Err() != nil {
		log.Println(result.Err().Error())
		return nil, "", result.Err()
	}

	inserted_Job := Job{}

	err = result.Decode(&inserted_Job)
	if err != nil {
		log.Println(err.Error())
		return nil, "", err
	}

	return &inserted_Job, secretID, nil
}

//UpdateK8s Updates a job with its k8s id
func (handler *Handler) UpdateK8s(id string, k8s string, conf string) error {
	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)

	update_filter := bson.M{
		"jobid": id,
	}

	update := bson.M{
		"$set": bson.M{
			"k8sid":  k8s,
			"status": api.JobStatusEnum_RUNNING.String(),
		},
	}

	result, err := handler.Collection.UpdateOne(ctx, update_filter, update)
	if err != nil {
		log.Println(err.Error())
		return err
	}

	if result.MatchedCount != 1 {
		err := fmt.Errorf("wrong number of updated job entries found when updating job: %v", id)
		log.Println(err.Error())
		return err
	}

	return nil
}

//UpdateStatus Updates the status of a job
func (handler *Handler) UpdateStatus(id string, status api.JobStatusEnum, errorMsg string, isDeleted bool) error {
	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)

	update_filter := bson.M{
		"jobid": id,
	}

	update := bson.M{
		"$set": bson.M{
			"error":   errorMsg,
			"status":  status.String(),
			"updated": primitive.Timestamp{T: uint32(time.Now().Unix())},
		},
	}

	result, err := handler.Collection.UpdateOne(ctx, update_filter, update)
	if err != nil {
		log.Println(err.Error())
		return err
	}

	if result.MatchedCount != 1 {
		err := fmt.Errorf("wrong number of updated job entries found when updating job: %v", id)
		log.Println(err.Error())
		return err
	}

	return nil
}

//GetJob Returns the stored config of a job
func (handler *Handler) GetJob(id string) (*Job, error) {
	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)

	find_query := bson.M{
		"jobid": id,
	}

	result := handler.Collection.FindOne(ctx, find_query)
	if result.Err() != nil {
		log.Println(result.Err().Error())
		return nil, result.Err()
	}

	job := Job{}

	err := result.Decode(&job)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return &job, nil
}

//CheckSecret Compares the provided secret/JobID with a job in the database
func (handler *Handler) CheckSecret(id string, secretKey string) error {
	job, err := handler.GetJob(id)
	if err != nil {
		log.Println(err.Error())
		return err
	}

	secretSHA := sha256.Sum256([]byte(secretKey))
	secretSHABase64 := base64.StdEncoding.EncodeToString(secretSHA[:])

	if secretSHABase64 != job.Secret {
		return errors.New("wrong secret provided")
	}

	return nil
}

// GetJobsStatus Returns the status of a list of jobs
func (handler *Handler) GetJobs(jobIDs []*api.JobAuth) ([]Job, error) {
	var ids []string
	var jobs []Job

	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)

	for _, id := range jobIDs {
		ids = append(ids, id.JobID)
	}

	find_query := bson.M{
		"jobid": bson.M{
			"$in": ids,
		},
	}

	csr, err := handler.Collection.Find(ctx, find_query)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	if err = csr.All(ctx, &jobs); err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return jobs, nil
}

// GetJobStatus Returns the status of an individual job
func (handler *Handler) GetJobStatus(jobID string) (*Job, error) {
	job, err := handler.GetJob(jobID)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return job, nil
}

func (handler *Handler) GetRunningJobs() ([]*Job, error) {
	var runningJobs []*Job
	running_jobs_query := bson.M{
		"Status": api.JobStatusEnum_RUNNING.String(),
	}

	ctx, _ := context.WithTimeout(context.Background(), 1*time.Second)

	csr, err := handler.Collection.Find(ctx, running_jobs_query)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	if err = csr.All(ctx, &runningJobs); err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return runningJobs, nil
}

func (handler *Handler) createUploadStoreKey(id string, uploadFileType UploadFileType) string {
	var filename string
	switch uploadFileType {
	case Fasta:
		filename = "fastadata.fasta"
	case RepliconTSV:
		filename = "replicons.tsv"
	case RepliconCSV:
		filename = "replicons.tsv"
	case Prodigal:
		filename = "prodigaltraining.tf"
	}

	resultKey := path.Join(handler.BaseKey, "uploaddata", id, filename)
	return resultKey
}

func (handler *Handler) createResultStoreKey(id string) string {
	resultKey := path.Join(handler.BaseKey, "results", id)
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
