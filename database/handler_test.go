package database

import (
	"context"
	"fmt"
	"log"
	"os"
	"testing"
	"time"

	"github.com/ag-computational-bio/bakta-web-api-go/api"
	"github.com/google/uuid"
	"github.com/spf13/viper"
	"go.mongodb.org/mongo-driver/bson"
)

func TestDatabaseHandler(t *testing.T) {
	databaseHandler, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	job1, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	job2, _, err := databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateK8s(job1.JobID, "test1")
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateK8s(job2.JobID, "test2")
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateStatus(job1.JobID, api.JobStatusEnum_RUNNING, "", false)
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateStatus(job2.JobID, api.JobStatusEnum_RUNNING, "", false)
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateStatus(job1.JobID, api.JobStatusEnum_SUCCESSFULL, "", true)
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.UpdateStatus(job2.JobID, api.JobStatusEnum_ERROR, "some random error", true)
	if err != nil {
		t.Errorf(err.Error())
	}

}

func TestDeletion(t *testing.T) {
	log.SetFlags(log.LstdFlags | log.Lshortfile)
	viper.SetConfigFile("../config/local-config.yaml")
	err := viper.ReadInConfig()
	if err != nil {
		log.Fatalln(err.Error())
	}

	err = os.Setenv("MongoPassword", "test123")
	if err != nil {
		log.Fatalln(err.Error())
	}

	databaseHandler, err := InitDatabaseHandler()
	if err != nil {
		t.Errorf(err.Error())
	}

	expiryTime := 1

	databaseHandler.ExpiryTime, err = time.ParseDuration(fmt.Sprintf("-%vs", expiryTime))
	if err != nil {
		t.Errorf(err.Error())
	}

	id := uuid.New().String()

	ctx, _ := context.WithTimeout(context.Background(), 5*time.Second)
	collection := databaseHandler.DB.Database("baktatest").Collection(id)
	err = collection.Drop(ctx)
	if err != nil {
		t.Errorf(err.Error())
	}

	databaseHandler.Collection = collection

	_, _, err = databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	_, _, err = databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	time.Sleep(1 * time.Second)

	_, _, err = databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	_, _, err = databaseHandler.CreateJob(api.RepliconTableType_csv)
	if err != nil {
		t.Errorf(err.Error())
	}

	err = databaseHandler.DeleteExpiredJobs()
	if err != nil {
		t.Errorf(err.Error())
	}

	query_remainers := bson.M{}

	csr, err := databaseHandler.Collection.Find(ctx, query_remainers)
	if err != nil {
		t.Errorf(err.Error())
	}
	remaining := csr.RemainingBatchLength()

	if remaining != 2 {
		t.Errorf("wrong number of jobs remaining after deletion")
	}

	collection = databaseHandler.DB.Database("baktatest").Collection(id)
	err = collection.Drop(ctx)
	if err != nil {
		t.Errorf(err.Error())
	}

}

func TestRndBytes(t *testing.T) {
	_, err := randStringBytes(50)
	if err != nil {
		t.Errorf(err.Error())
	}
}
