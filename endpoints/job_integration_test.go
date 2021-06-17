// +/build integration

package endpoints

import (
	"context"
	"fmt"
	proto_api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
	"github.com/spf13/viper"
	"log"
	"net"
	"testing"
)

func InitAPI() *BaktaJobAPI {
	dbHandler, err := database.InitDatabaseHandler()
	if err != nil {
		log.Fatalln(err.Error())
	}

	clientset, err := scheduler.CreateClientSet()

	if err != nil {
		log.Fatalln(err.Error())
	}

	sched, err := scheduler.InitSimpleScheduler(dbHandler, clientset)
	if err != nil {
		log.Fatalln(err.Error())
	}

	bucket := viper.GetString("Objectstorage.S3.Bucket")

	s3Handler, err := objectStorage.InitS3ObjectStorageHandler(bucket)
	if err != nil {
		log.Println(fmt.Sprintf("failed to listen: %v", err))
		return err
	}

	updateMonitor := monitor.New(sched.GetK8sClient(), sched.GetNamespace())

	return InitBaktaAPI(dbHandler, sched, s3Handler, &updateMonitor)

}

func TestBaktaJobAPI_InitJob(t *testing.T) {
	api := InitAPI()
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})

}

func TestBaktaJobAPI_StartJob(t *testing.T) {
	api := InitAPI()
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})

}

func TestBaktaJobAPI_JobsStatus(t *testing.T) {
	api := InitAPI()
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})

}

func TestBaktaJobAPI_JobResult(t *testing.T) {
	api := InitAPI()
	response, err := api.InitJob(context.Background(), &proto_api.InitJobRequest{
		RepliconTableType: 0,
		Name:              "",
	})

}
